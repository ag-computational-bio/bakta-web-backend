package endpoints

import (
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"log"
	"os"
	"time"

	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
	"github.com/ag-computational-bio/bakta-web-backend/monitor"
	"github.com/ag-computational-bio/bakta-web-backend/objectStorage"
	"google.golang.org/protobuf/types/known/structpb"
	"google.golang.org/protobuf/types/known/timestamppb"

	"github.com/ag-computational-bio/bakta-web-backend/database"
	"github.com/ag-computational-bio/bakta-web-backend/scheduler"
)

//BaktaJobAPI implements the job endpoints of the bakta-web-api
type BaktaJobAPI struct {
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
	job, secret, err := apiHandler.dbHandler.CreateJob(request.RepliconTableType, request.GetName())
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

	k8sJob, err := apiHandler.scheduler.StartJob(request.Job.GetJobID(), request.GetConfig())
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	err = apiHandler.dbHandler.UpdateK8s(request.Job.GetJobID(), string(k8sJob.GetUID()))
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	return &api.Empty{}, nil
}

//GetJobsStatus Get the job status of the provided list of jobs
func (apiHandler *BaktaJobAPI) JobsStatus(ctx context.Context, request *api.JobStatusRequestList) (*api.JobStatusReponseList, error) {
	var failedJobs []*api.FailedJob
	var jobsStatus []*api.JobStatusResponse

	jobs, err := apiHandler.dbHandler.GetJobs(request.GetJobs())
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	foundJobs := make(map[string]database.Job)
	for _, job := range jobs {
		foundJobs[job.JobID] = job

	}

	for _, expectedJob := range request.GetJobs() {
		job, ok := foundJobs[expectedJob.JobID]
		if !ok {
			failedJobs = append(failedJobs, &api.FailedJob{
				JobID:     expectedJob.JobID,
				JobStatus: api.JobFailedStatus_NOT_FOUND,
			})
			continue
		}

		secretSHA := sha256.Sum256([]byte(expectedJob.Secret))
		secretSHABase64 := base64.StdEncoding.EncodeToString(secretSHA[:])

		if secretSHABase64 != job.Secret {
			failedJobs = append(failedJobs, &api.FailedJob{
				JobID:     expectedJob.JobID,
				JobStatus: api.JobFailedStatus_UNAUTHORIZED,
			})
			continue
		}

		statusNumber, ok := api.JobStatusEnum_value[job.Status]
		if !ok {
			err = fmt.Errorf("%v not a valid status", job.Status)
			return nil, err
		}

		statusEnum := api.JobStatusEnum(statusNumber)

		created_time := timestamppb.New(time.Unix(int64(job.Created.T), 0))
		updated_time := timestamppb.New(time.Unix(int64(job.Updated.T), 0))

		statusResponse := api.JobStatusResponse{
			JobID:     job.JobID,
			JobStatus: statusEnum,
			Started:   created_time,
			Updated:   updated_time,
			Name:      job.Jobname,
		}

		jobsStatus = append(jobsStatus, &statusResponse)
	}

	response := api.JobStatusReponseList{
		Jobs:       jobsStatus,
		FailedJobs: failedJobs,
	}

	return &response, nil
}

//GetJobResult Returns the results for a specific jobs
func (apiHandler *BaktaJobAPI) JobResult(ctx context.Context, request *api.JobAuth) (*api.JobResultResponse, error) {
	err := apiHandler.dbHandler.CheckSecret(request.GetJobID(), request.GetSecret())
	if err != nil {
		err = fmt.Errorf("JobID does not match secret ID")
		return nil, err
	}

	job, err := apiHandler.dbHandler.GetJobStatus(request.GetJobID())
	if err != nil {
		log.Println(err.Error())
		return nil, fmt.Errorf("could not read job result for job: %v", request.GetJobID())
	}

	results, err := apiHandler.s3Handler.CreateDownloadLinks(job.DataBucket, job.ResultKey, "result")
	if err != nil {
		log.Println(err.Error())
		return nil, fmt.Errorf("could not create download url for job: %v", request.GetJobID())
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

	created_time := timestamppb.New(time.Unix(int64(job.Created.T), 0))
	updated_time := timestamppb.New(time.Unix(int64(job.Updated.T), 0))

	jobResponse := api.JobResultResponse{
		JobID:       job.JobID,
		ResultFiles: &structData,
		Started:     created_time,
		Updated:     updated_time,
		Name:        job.Jobname,
	}

	return &jobResponse, nil
}

func (apiHandler *BaktaJobAPI) Version(ctx context.Context, request *api.Empty) (*api.VersionResponse, error) {
	shaVersion := os.Getenv("GITHUB_SHA")

	version := api.VersionResponse{
		ToolVersion:    "1.0.4",
		DbVersion:      "2.0.0",
		BackendVersion: shaVersion,
	}

	return &version, nil
}

func (apiHandler *BaktaJobAPI) Delete(context.Context, *api.JobAuth) (*api.Empty, error) {
	return &api.Empty{}, nil
}

func pretty(value interface{}) {
	empJSON, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		log.Fatalf(err.Error())
	}
	fmt.Printf("%s\n", string(empJSON))
}
