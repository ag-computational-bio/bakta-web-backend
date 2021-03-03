package endpoints

import (
	"context"
	"fmt"
	"log"

	"github.com/ag-computational-bio/bakta-web-backend/objectStorage"
	"google.golang.org/protobuf/types/known/structpb"

	"github.com/ag-computational-bio/bakta-web-backend/database"
	"github.com/ag-computational-bio/bakta-web-backend/scheduler"

	"github.com/ag-computational-bio/bakta-web-api/go/api"
)

//BaktaJobAPI implements the job endpoints of the bakta-web-api
type BaktaJobAPI struct {
	api.UnimplementedBaktaJobsServer
	dbHandler *database.Handler
	scheduler *scheduler.SimpleScheduler
	s3Handler *objectStorage.S3ObjectStorageHandler
}

//InitBaktaAPI Initiates the Bakta API handler
func InitBaktaAPI(dbHandler *database.Handler, scheduler *scheduler.SimpleScheduler, s3Handler *objectStorage.S3ObjectStorageHandler) *BaktaJobAPI {
	api := &BaktaJobAPI{
		dbHandler: dbHandler,
		scheduler: scheduler,
		s3Handler: s3Handler,
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

	prodigalUploadKey, err := apiHandler.s3Handler.CreateUploadLink(job.DataBucket, job.FastaKey)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	repliconsUploadKey, err := apiHandler.s3Handler.CreateUploadLink(job.DataBucket, job.FastaKey)
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

	k8sJob, err := apiHandler.scheduler.StartJob(request.Job.GetJobID(), request.GetConfig())
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
		err := apiHandler.dbHandler.CheckSecret(jobID.GetJobID(), jobID.GetSecret())
		if err != nil {
			err = fmt.Errorf("JobID does not match secret ID")
			return nil, err
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

		statusResponse := api.JobStatusResponse{
			JobID:     job.JobID,
			JobStatus: statusEnum,
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

	structpb, err := structpb.NewValue(&results)
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	jobResponse := api.JobResultResponse{
		JobID:       job.JobID,
		ResultFiles: structpb.GetStructValue(),
	}

	return &jobResponse, nil
}

func (apiHandler *BaktaJobAPI) Version(ctx context.Context, request *api.Empty) (*api.VersionResponse, error) {
	version := api.VersionResponse{
		ToolVersion: "0.0.1",
		DbVersion:   "1.1.x",
	}

	return &version, nil
}
