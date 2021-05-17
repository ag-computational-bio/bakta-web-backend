package endpoints

import (
	"context"
	"encoding/json"
	"fmt"
	"log"
	"os"
	"time"

	"github.com/ag-computational-bio/bakta-web-api-go/api"
	"github.com/ag-computational-bio/bakta-web-backend/monitor"
	"github.com/ag-computational-bio/bakta-web-backend/objectStorage"
	"google.golang.org/protobuf/types/known/structpb"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/ag-computational-bio/bakta-web-backend/database"
	"github.com/ag-computational-bio/bakta-web-backend/scheduler"
)

//BaktaJobAPI implements the job endpoints of the bakta-web-api
type BaktaJobAPI struct {
	api.UnimplementedBaktaJobsServer
	dbHandler *database.Handler
	scheduler *scheduler.SimpleScheduler
	s3Handler *objectStorage.S3ObjectStorageHandler
	monitor   *monitor.SimpleMonitor
}

//InitBaktaAPI Initiates the Bakta API handler
func InitBaktaAPI(dbHandler *database.Handler, scheduler *scheduler.SimpleScheduler, s3Handler *objectStorage.S3ObjectStorageHandler, monitor *monitor.SimpleMonitor) *BaktaJobAPI {
	api := &BaktaJobAPI{
		dbHandler: dbHandler,
		scheduler: scheduler,
		s3Handler: s3Handler,
		monitor:   monitor,
	}

	return api
}

//InitJob Initiates a bakta job and returns upload links for the fasta, prodigal training and replicon file
func (apiHandler *BaktaJobAPI) InitJob(ctx context.Context, request *api.InitJobRequest) (*api.InitJobResponse, error) {
	job, secret, err := apiHandler.dbHandler.CreateJob()
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	fastaUploadKey, err := apiHandler.s3Handler.CreateUploadLink(job.DataBucket, job.FastaKey)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	prodigalUploadKey, err := apiHandler.s3Handler.CreateUploadLink(job.DataBucket, job.ProdigalKey)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	repliconsUploadKey, err := apiHandler.s3Handler.CreateUploadLink(job.DataBucket, job.RepliconKey)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	initJobResp := api.InitJobResponse{
		Job: &api.JobAuth{
			JobID:  job.JobID,
			Secret: secret,
		},
		UploadLinkFasta:     fastaUploadKey,
		UploadLinkProdigal:  prodigalUploadKey,
		UploadLinkReplicons: repliconsUploadKey,
	}

	return &initJobResp, nil
}

//StartJob Starts a job based on the provided configuration
func (apiHandler *BaktaJobAPI) StartJob(ctx context.Context, request *api.StartJobRequest) (*api.Empty, error) {
	err := apiHandler.dbHandler.CheckSecret(request.GetJob().GetJobID(), request.GetJob().GetSecret())
	if err != nil {
		err = fmt.Errorf("JobID does not match secret ID")
		return nil, err
	}

	k8sJob, err := apiHandler.scheduler.StartJob(request.Job.GetJobID(), request.GetConfig(), request.GetJobConfigString())
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	err = apiHandler.dbHandler.UpdateK8s(request.Job.GetJobID(), string(k8sJob.GetUID()), request.GetJobConfigString())
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	return &api.Empty{}, nil
}

//GetJobsStatus Get the job status of the provided list of jobs
func (apiHandler *BaktaJobAPI) GetJobsStatus(ctx context.Context, request *api.JobStatusRequestList) (*api.JobStatusReponseList, error) {
	var jobIDs []string

	for _, jobID := range request.GetJobs() {
		isDeleted := false

		err := apiHandler.dbHandler.CheckSecret(jobID.GetJobID(), jobID.GetSecret())
		if err != nil {
			err = fmt.Errorf("JobID does not match secret ID")
			return nil, err
		}

		job, err := apiHandler.dbHandler.GetJob(jobID.GetJobID())
		if err != nil {
			err = fmt.Errorf("could not find job")
			return nil, err
		}

		newStatus, err := apiHandler.monitor.GetJobStatus(jobID.GetJobID())
		if err != nil {
			err = fmt.Errorf("could not get updated job status")
			return nil, err
		}

		if newStatus.Status == api.JobStatusEnum_SUCCESSFULL || newStatus.Status == api.JobStatusEnum_ERROR {
			err := apiHandler.scheduler.DeleteJob(job.K8sID)
			if err != nil {
				err = fmt.Errorf("could not get updated job status")
				return nil, err
			}

			isDeleted = true
		}

		if job.Status != newStatus.Status.String() {
			err = apiHandler.dbHandler.UpdateStatus(jobID.GetJobID(), newStatus.Status, newStatus.ErrorMsg, isDeleted)
			if err != nil {
				err = fmt.Errorf("could not update job status")
				return nil, err
			}
		}

		jobIDs = append(jobIDs, jobID.JobID)
	}

	jobs, err := apiHandler.dbHandler.GetJobsStatus(jobIDs)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	var jobsStatus []*api.JobStatusResponse
	for _, job := range jobs {
		statusNumber, ok := api.JobStatusEnum_value[job.Status]
		if !ok {
			err = fmt.Errorf("%v not a valid status", job.Status)
			return nil, err
		}

		statusEnum := api.JobStatusEnum(statusNumber)

		created_time := timestamppb.New(time.Unix(job.Created, 0))
		updated_time := timestamppb.New(time.Unix(job.Updated, 0))

		statusResponse := api.JobStatusResponse{
			JobID:     job.JobID,
			JobStatus: statusEnum,
			Started:   created_time,
			Updated:   updated_time,
		}

		jobsStatus = append(jobsStatus, &statusResponse)
	}

	reponse := api.JobStatusReponseList{
		Jobs: jobsStatus,
	}

	return &reponse, nil
}

//GetJobResult Returns the results for a specific jobs
func (apiHandler *BaktaJobAPI) GetJobResult(ctx context.Context, request *api.JobAuth) (*api.JobResultResponse, error) {
	err := apiHandler.dbHandler.CheckSecret(request.GetJobID(), request.GetSecret())
	if err != nil {
		err = fmt.Errorf("JobID does not match secret ID")
		return nil, err
	}

	job, err := apiHandler.dbHandler.GetJobStatus(request.GetJobID())
	if err != nil {
		log.Println(err.Error())
		return nil, fmt.Errorf("Could not read job result for job: %v", request.GetJobID())
	}

	results, err := apiHandler.s3Handler.CreateDownloadLinks(job.DataBucket, job.ResultKey, "result")
	if err != nil {
		log.Println(err.Error())
		return nil, fmt.Errorf("Could not create download url for job: %v", request.GetJobID())
	}

	intermediateByteData, err := json.Marshal(results)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	var structData structpb.Struct
	err = structData.UnmarshalJSON(intermediateByteData)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	created_time := timestamppb.New(time.Unix(job.Created, 0))
	updated_time := timestamppb.New(time.Unix(job.Updated, 0))

	jobResponse := api.JobResultResponse{
		JobID:       job.JobID,
		ResultFiles: &structData,
		Started:     created_time,
		Updated:     updated_time,
	}

	return &jobResponse, nil
}

func (apiHandler *BaktaJobAPI) Version(ctx context.Context, request *api.Empty) (*api.VersionResponse, error) {
	shaVersion := os.Getenv("GITHUB_SHA")

	version := api.VersionResponse{
		ToolVersion:    "1.0.0",
		DbVersion:      "2.0.0",
		BackendVersion: shaVersion,
	}

	return &version, nil
}
