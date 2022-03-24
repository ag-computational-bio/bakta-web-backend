package endpoints

import (
	"context"
	"encoding/json"
	"fmt"
	api "github.com/ag-computational-bio/bakta-web-api-go/bakta/web/api/proto/v1"
	"github.com/ag-computational-bio/bakta-web-backend/argoclient"
	"github.com/ag-computational-bio/bakta-web-backend/objectStorage"
	"github.com/spf13/viper"
	"google.golang.org/protobuf/types/known/structpb"
	"google.golang.org/protobuf/types/known/timestamppb"
	"log"
	"os"
)

//BaktaJobAPI implements the job endpoints of the bakta-web-api
type BaktaJobAPI struct {
	s3Handler     *objectStorage.S3ObjectStorageHandler
	statusHandler *argoclient.StatusHandler
}

//InitBaktaAPI Initiates the Bakta API handler
func InitBaktaAPI(statusHandler *argoclient.StatusHandler, s3Handler *objectStorage.S3ObjectStorageHandler) *BaktaJobAPI {
	return &BaktaJobAPI{
		statusHandler: statusHandler,
		s3Handler:     s3Handler,
	}
}

//InitJob Initiates a bakta job and returns upload links for the fasta, prodigal training and replicon file
func (apiHandler *BaktaJobAPI) InitJob(ctx context.Context, request *api.InitJobRequest) (*api.InitJobResponse, error) {
	jobID, secret, err := apiHandler.statusHandler.InitJob(request.GetName())
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	fastaUploadKey, err := apiHandler.s3Handler.CreateUploadLink(apiHandler.s3Handler.CreateKeyPath(jobID, "inputs", "fastadata.fasta"))
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	prodigalUploadKey, err := apiHandler.s3Handler.CreateUploadLink(apiHandler.s3Handler.CreateKeyPath(jobID, "inputs", "prodigal.tf"))
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	repliconsUploadKey, err := apiHandler.s3Handler.CreateUploadLink(apiHandler.s3Handler.CreateKeyPath(jobID, "inputs", "replicons.tsv"))
	if err != nil {
		log.Println(err.Error())
		return nil, err
	}

	initJobResp := api.InitJobResponse{
		Job: &api.JobAuth{
			JobID:  jobID,
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
	err := apiHandler.statusHandler.StartJob(request.Job.JobID, request.Job.Secret, request.GetConfig())
	if err != nil {
		return nil, err
	}
	return &api.Empty{}, nil
}

//JobsStatus Get the job status of the provided list of jobs
func (apiHandler *BaktaJobAPI) JobsStatus(ctx context.Context, request *api.JobStatusRequestList) (*api.JobStatusReponseList, error) {
	var failedJobs []*api.FailedJob
	var jobsStatus []*api.JobStatusResponse

	for _, jobS := range request.GetJobs() {

		wfstatus, err := apiHandler.statusHandler.GetJob(jobS.GetJobID(), jobS.GetSecret())
		if err != nil {
			log.Println(fmt.Sprintf("getwfstatus error: %v", err))
			continue
		}

		jobsStatus = append(jobsStatus, &api.JobStatusResponse{
			JobID:     wfstatus.JobId,
			JobStatus: apiHandler.statusHandler.ParseStatus(wfstatus.Status),
			Started:   timestamppb.New(wfstatus.Started),
			Updated:   timestamppb.New(wfstatus.Updated),
			Name:      wfstatus.Name,
		})
	}

	response := api.JobStatusReponseList{
		Jobs:       jobsStatus,
		FailedJobs: failedJobs,
	}

	return &response, nil
}

//JobResult Returns the results for a specific jobs
func (apiHandler *BaktaJobAPI) JobResult(ctx context.Context, request *api.JobAuth) (*api.JobResultResponse, error) {
	jobstatus, err := apiHandler.statusHandler.GetJob(request.GetJobID(), request.GetSecret())
	if err != nil {
		err = fmt.Errorf("JobID does not match secret ID")
		return nil, err
	}

	results, err := apiHandler.s3Handler.CreateDownloadLinks(jobstatus.JobId)
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

	jobResponse := api.JobResultResponse{
		JobID:       jobstatus.JobId,
		ResultFiles: &structData,
		Started:     timestamppb.New(jobstatus.Started),
		Updated:     timestamppb.New(jobstatus.Updated),
		Name:        jobstatus.Name,
	}

	return &jobResponse, nil
}

func (apiHandler *BaktaJobAPI) Version(ctx context.Context, request *api.Empty) (*api.VersionResponse, error) {
	shaVersion := os.Getenv("GITHUB_SHA")

	version := api.VersionResponse{
		ToolVersion:    viper.GetString("Version.Tool"),
		DbVersion:      viper.GetString("Version.DB"),
		BackendVersion: shaVersion,
	}

	return &version, nil
}

func (apiHandler *BaktaJobAPI) Delete(ctx context.Context, job *api.JobAuth) (*api.Empty, error) {
	err := apiHandler.statusHandler.DeleteJob(job.JobID, job.Secret)
	if err != nil {
		return nil, err
	}
	return &api.Empty{}, nil
}
